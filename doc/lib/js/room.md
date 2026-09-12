---
title: "@moq/room"
description: Headless multi-participant rooms over MoQ
---

# @moq/room

[![npm](https://img.shields.io/npm/v/@moq/room)](https://www.npmjs.com/package/@moq/room)

A room is a path prefix. There is no service and no storage: joining is minting
a moq-token rooted at that prefix and dialing the relay. Participants are
discovered from the announce stream. Identity is the path before `camera` /
`screen`. Each participant publishes `{identity}/camera` (camera + mic, hd/sd)
and `{identity}/screen` (screenshare).

```ts
import { Local, Room } from "@moq/room";
import { Connection, Path } from "@moq/net";
import { claims } from "@moq/room";
import { sign } from "@moq/token";

const token = await sign(key, claims("meet/demo", "alice"));
const connection = new Connection.Reload({
    url: new URL(`https://relay.example.com/meet/demo?jwt=${token}`),
    enabled: true,
});

const local = new Local({
    connection: connection.established,
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

hang.live should depend on this package for the roster, local publish, remote
watch, and `hang/user.json` + `hang/preview.json`. Chat and location stay
app-defined extensions of the same catalog `hang` section.

See the package [README](https://github.com/moq-dev/moq/blob/main/js/room/README.md)
for the full API.
