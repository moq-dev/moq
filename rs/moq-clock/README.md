<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-clock

A tiny example app that publishes or subscribes to a clock track over [Media over QUIC](https://moq.dev).

Useful as a reference for [moq-net](https://github.com/moq-dev/moq/tree/main/rs/moq-net) usage and for sanity-checking relay connectivity and latency. The JS port lives in [@moq/clock](https://github.com/moq-dev/moq/tree/main/js/clock).

This is an example binary, not a distributed crate. Build it from a checkout of the workspace:

```bash
cargo run -p moq-clock -- --url https://relay.example.com/anon --broadcast clock publish

# In another shell
cargo run -p moq-clock -- --url https://relay.example.com/anon --broadcast clock subscribe
```
