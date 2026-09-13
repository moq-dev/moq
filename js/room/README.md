<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/room

[![npm version](https://img.shields.io/npm/v/@moq/room)](https://www.npmjs.com/package/@moq/room)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Headless multi-participant rooms over [Media over QUIC](https://moq.dev/). A room is a path prefix. There is no service and no storage: joining is minting a moq-token rooted at that prefix (the LiveKit AccessToken analogue) and dialing the relay.

Participants are discovered from the announce stream. Identity is the path before `camera.hang` / `screen.hang`. Each participant publishes:

- `{identity}/camera.hang`: camera + microphone, hd/sd renditions
- `{identity}/screen.hang`: screenshare; its announce/unannounce is the share lifecycle

This is the generic room layer extracted from [hang.live](https://hang.live) (roster, local/remote, `hang/*.json` metadata) and [iroh-live](https://github.com/n0-computer/iroh-live) (the `chat` track `iroh-rooms` is moving onto the announce bus). Memes, 3D layout, chat UI, and accounts stay in the app. The native twin is [`moq-room`](../../rs/moq-room).

## Install

```bash
bun add @moq/room
```

## Token

Sign with [`@moq/token`](../token). `root` is the room, `get: ""` subscribes to everyone, `put: "<identity>/"` so a participant cannot publish at someone else's paths.

```ts
import { claims } from "@moq/room";
import { sign } from "@moq/token";

const token = await sign(key, claims("meet/demo", "alice"));
// Dial https://relay.example.com/meet/demo?jwt=<token>
```

On a public prefix (`anon/`), skip the token and dial that path directly.

## Usage

```ts
import { Local, Room } from "@moq/room";
import { Connection, Path } from "@moq/net";

const connection = new Connection.Reload({
	url: new URL("https://relay.example.com/anon/meet/demo"),
	enabled: true,
});

const identity = Path.from("alice");
const local = new Local({
	connection: connection.established,
	identity,
	user: { name: "Alice" },
});
local.enabled.set(true);
local.cameraEnabled.set(true);
local.microphoneEnabled.set(true);

const room = new Room({ connection, identity });

// room.remotes is a Map<identity, Remote>. Each Remote has camera/screen
// Members; assign member.canvas and set member.muted to false to play audio.
```

hang.live should depend on this package for `Room`, `Local`, `Remote`, and the `hang/*.json` metadata tracks. Location stays an app-defined catalog extension (`TRACK.location`). hang.live's JSON chat (`TRACK.chat` = `hang/chat.json`) is also an extension; the JSON window track is `Chat.TRACK` (`"chat"`).

```ts
import { Chat } from "@moq/room";

const publisher = Chat.Publisher.create(broadcast);
publisher.send("hello");

const subscriber = Chat.Subscriber.subscribe(broadcast.consume());
const event = await subscriber.recv(); // push, pop, or skip
// Call publisher.finish() and subscriber.close() when done.
```

A conferencing demo (no memes, no 3D, no chat UI) lives at [`demo/web/src/meet.html`](../../demo/web/src/meet.html).

Room members start muted; set `member.muted` to `false` to play audio. Chat uses uncompressed JSON strings, a retained ten-second window with push/pop/skip events; it is not compatible with the raw UTF-8 iroh-live track. Empty normalized identities are rejected by `claims`.
