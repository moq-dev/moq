---
title: Server-side JS
description: Running @moq/net in Node, Bun, and Deno outside the browser
---

# Server-side JS

`@moq/net` runs outside the browser too, in Node, Bun, and Deno. This is handy
for bots, recorders, transcoders, test harnesses, and anything that needs to
publish or subscribe without a tab open.

`@moq/hang` is browser-only. It leans on WebCodecs, WebAudio, and the DOM, so
keep server-side work at the `@moq/net` layer (raw broadcasts, tracks, groups,
and frames) and do any media encode/decode yourself.

## Install

```bash
bun add @moq/net
# or
npm add @moq/net
```

## Transport

The browser gives `@moq/net` a native `WebTransport`. Server runtimes don't have
one yet, so `@moq/net` falls back to **WebSocket** and talks to the relay over
its WebSocket endpoint instead. No code change required, the relay accepts both.

- **Bun** and **Node 21+** ship `WebSocket` globally, so nothing else is needed.
- **Older Node** has no global `WebSocket`. Add the [`ws`](https://www.npmjs.com/package/ws)
  polyfill before importing `@moq/net`:

  ```javascript
  import WebSocket from "ws";
  globalThis.WebSocket = WebSocket;

  import * as Moq from "@moq/net";
  ```

### Optional: native WebTransport

WebSocket is the path of least resistance and works everywhere. If you'd rather
keep the QUIC/HTTP3 transport on the server (lower overhead, datagram support),
install the [`@moq/web-transport`](https://www.npmjs.com/package/@moq/web-transport)
polyfill, which provides a native WebTransport via NAPI, and register it before
importing `@moq/net`:

```bash
bun add @moq/web-transport
```

```javascript
import { install } from "@moq/web-transport";
install(); // sets globalThis.WebTransport

import * as Moq from "@moq/net";
```

::: tip Runtime support
Bun and Deno are the smoothest server-side targets today. Node works over
WebSocket; the native WebTransport polyfill is the newer path and is still
settling, so reach for it only if you specifically need QUIC server-side.
:::

## Connect

Once a transport is in place, the API is identical to the browser:

```javascript
import * as Moq from "@moq/net";

const url = new URL("https://relay.moq.dev/anon");
const connection = await Moq.Connection.connect(url);

// publish or subscribe exactly as you would in the browser...

await connection.close();
```

See the [`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples)
directory for runnable [connection](https://github.com/moq-dev/moq/blob/main/js/net/examples/connection.ts),
[publish](https://github.com/moq-dev/moq/blob/main/js/net/examples/publish.ts),
[subscribe](https://github.com/moq-dev/moq/blob/main/js/net/examples/subscribe.ts),
and [discovery](https://github.com/moq-dev/moq/blob/main/js/net/examples/discovery.ts)
scripts. Run them with `bun` or `tsx`.

## Next steps

- [@moq/net](/lib/js/@moq/net) - Core protocol API
- [Web Components](/lib/js/env/web) - The browser story
- [moq-relay](/bin/relay/) - The relay these clients connect to
