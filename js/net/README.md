<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/net

[![npm version](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript [Media over QUIC](https://moq.dev/) (MoQ) client for both browsers and server JS/TS environments.
`@moq/net` is the **networking layer**: real-time pub/sub with built-in caching, fan-out, and prioritization, on top of QUIC. At session setup it negotiates one of two wire protocols, either the simplified [moq-lite](https://doc.moq.dev/concept/layer/moq-lite) protocol or the full IETF [moq-transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) draft.

Check out [hang](../hang) for a higher-level media library that uses this package.

> **Note:** moq-lite is a subset of moq-transport and is forwards compatible with it, so this client works with any moq-transport CDN (ex. [Cloudflare](https://moq.dev/blog/first-cdn/)). See the [compatibility docs](https://doc.moq.dev/concept/layer/moq-lite#compatibility) for details.

## Quick Start

```bash
npm add @moq/net
# or
pnpm add @moq/net
bun add @moq/net
yarn add @moq/net
# etc
```

## Server-side usage

`@moq/net` works on both browsers and servers, however in JS/TS server environments (Node, Bun) WebTransport is not yet available, so `@moq/net` will default to WebSockets communication with the relay.

Bun and Node v21+ have `WebSockets` built in, but older versions of Node do not, so for older versions of Node you will need the WebSockets polyfill to use `@moq/net`

```javascript
import WebSocket from 'ws';
import * as Moq from '@moq/net';
// Polyfill WebSocket for MoQ
globalThis.WebSocket = WebSocket;
```

You can optionally enable `WebTransport` and full HTTP3/Quic on server environments with the following (experimental) [polyfill](https://github.com/fails-components/webtransport)

```bash
npm install @fails-components/webtransport
npm install @fails-components/webtransport-transport-http3-quiche
```

Which you would load as follows

```javascript
import { WebTransport, quicheLoaded } from '@fails-components/webtransport';
global.WebTransport = WebTransport;
import * as Moq from '@moq/net'
await quicheLoaded; //This is a promise, connect after it resolves
```

## Examples

- **[Connection](examples/connection.ts)** - Connect to a MoQ relay server
- **[Publishing](examples/publish.ts)** - Publish data to a broadcast
- **[Subscribing](examples/subscribe.ts)** - Subscribe to and receive broadcast data
- **[Discovery](examples/discovery.ts)** - Discover broadcasts announced by the server
- **[Server side usage](https://github.com/sb2702/webcodecs-examples/tree/main/src/moq-server)** - Publish from browser to a server

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)
