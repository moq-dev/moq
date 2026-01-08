<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/lite

[![npm version](https://img.shields.io/npm/v/@moq/lite)](https://www.npmjs.com/package/@moq/lite)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript implementation of [Media over QUIC](https://moq.dev/) (MoQ) providing real-time data delivery in web browsers.
Specificially, this package implements the networking layer called [moq-lite](https://moq.dev/blog/moq-lite).
Check out [../hang] for a higher-level media library that uses this package.

> **Note:** This project is a [fork](https://moq.dev/blog/transfork) of the [IETF MoQ specification](https://datatracker.ietf.org/group/moq/documents/), optimized for practical deployment with a narrower focus and exponentially simpler implementation.

## Quick Start

```bash
npm add @moq/lite
# or
pnpm add @moq/lite
bun add @moq/lite
yarn add @moq/lite
# etc
```

## Examples

- **[Connection](examples/connection.ts)** - Connect to a MoQ relay server
- **[Publishing](examples/publish.ts)** - Publish data to a broadcast
- **[Subscribing](examples/subscribe.ts)** - Subscribe to and receive broadcast data
- **[Discovery](examples/discovery.ts)** - Discover broadcasts announced by the server

## License

Licensed under either:

-   Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
