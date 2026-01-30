<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/lite

[![npm version](https://img.shields.io/npm/v/@moq/lite)](https://www.npmjs.com/package/@moq/lite)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript [Media over QUIC](https://moq.dev/) (MoQ) client for both browsers and server JS/TS environments.
The `@moq/lite` client specifically implements the networking layer called [moq-lite](https://moq.dev/blog/moq-lite), handling real-time data delivery to/from moq relays.


Check out [hang](../hang) for a higher-level media library that uses this package.

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

## Server-side usage

`@moq/lite` works on both browsers and servers, however in JS/TS server environments (Node, Bun) WebTransport is not yet available, so `@moq/lite` will default to WebSockets communication with the relay.

Bun and Node v21+ have `WebSockets` built in, but older versions of Node do not, so for older versions of Node you will need the WebSockets polyfill to use `@moq/lite`

```javascript
import WebSocket from 'ws';
import * as Moq from '@moq/lite';
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
import * as Moq from '@moq/lite'
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

-   Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

