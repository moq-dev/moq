<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/room

[![npm version](https://img.shields.io/npm/v/@moq/room)](https://www.npmjs.com/package/@moq/room)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Headless multi-participant rooms over [Media over QUIC](https://moq.dev/). A room is a path prefix. There is no service and no storage: joining is minting a moq-token rooted at that prefix (the LiveKit AccessToken analogue) and dialing the relay.

Participants are discovered from the announce stream. Identity is the path before `camera` / `screen`. Each participant publishes:

- `{identity}/camera` — camera + microphone, hd/sd renditions
- `{identity}/screen` — screenshare; its announce/unannounce is the share lifecycle

This is the generic room layer extracted from [hang.live](https://hang.live) (and the announce-bus shape [iroh-live](https://github.com/n0-computer/iroh-live) is moving `iroh-rooms` onto). Memes, 3D layout, chat UI, and accounts stay in the app.

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
// Members; assign member.canvas and member.muted from the UI.
```

hang.live should depend on this package for `Room`, `Local`, `Remote`, and the `hang/*.json` metadata tracks. Chat and location stay app-defined extensions of the same catalog `hang` section (`TRACK.chat`, `TRACK.location`); call `broadcast.catalog.mutate` to advertise extra tracks next to `user` and `preview`.

A conferencing demo (no memes, no 3D) lives at [`demo/web/src/meet.html`](../../demo/web/src/meet.html).
