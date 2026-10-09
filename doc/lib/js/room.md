---
title: "@moq/room"
description: Headless multi-participant rooms over MoQ
---

# @moq/room

[![npm](https://img.shields.io/npm/v/@moq/room)](https://www.npmjs.com/package/@moq/room)

A room is a path prefix. There is no service and no storage: joining is minting
a moq-auth token rooted at that prefix and dialing the relay. Participants are
discovered from the announce stream. Identity is the path before `camera.hang` /
`screen.hang`. Each participant publishes `{identity}/camera.hang` (camera + mic, hd/sd)
and `{identity}/screen.hang` (screenshare).

```ts
import { claims, Local, Room } from "@moq/room";
import { Connection, Path } from "@moq/net";
import { Key } from "@moq/auth";

const token = await Key.sign(key, claims("meet/demo", "alice"));
const connection = new Connection({
    url: new URL(`https://relay.example.com/meet/demo?jwt=${token}`),
    enabled: true,
});

const local = new Local({
    connection,
    identity: Path.from("alice"),
    user: { name: "Alice" },
});
local.enabled.set(true);
local.cameraEnabled.set(true);

const room = new Room({ connection, identity: Path.from("alice") });
```

On a public prefix, skip the token and dial that path directly. The conferencing
demo at [`demo/web`](https://github.com/moq-dev/moq/tree/main/demo/web) (`meet.html`)
does that under `anon/meet/{room}`.

Each remote member owns a `Watch.Player` and starts muted. Display video with
`member.canvas.set(canvas)` and unmute with `member.muted.set(false)`, or use
`member.player` for the full pipeline. `Chat` adds a text chat track with a
short retained history.

Under a CSP that refuses `blob:`, host the worklets and worker as described
for [watch](/lib/js/watch#strict-csp) and [publish](/lib/js/publish#strict-csp),
then call both `Watch.assets()` and `Publish.assets()`.

The native twin is [`moq-room`](/lib/rs/moq-room).

See the package [README](https://github.com/moq-dev/moq/blob/main/js/room/README.md)
for the full API.
