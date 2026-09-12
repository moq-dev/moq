---
title: moq-room
description: Headless multi-participant rooms over MoQ
---

# moq-room

[![crates.io](https://img.shields.io/crates/v/moq-room)](https://crates.io/crates/moq-room)
[![docs.rs](https://docs.rs/moq-room/badge.svg)](https://docs.rs/moq-room)

The native twin of [`@moq/room`](/lib/js/room). A room is a path prefix. There
is no service and no storage: joining is minting a moq-token rooted at that
prefix and dialing the relay. Participants are discovered from the announce
stream. Identity is the path before `camera` / `screen`.

Each participant publishes `{identity}/camera` (camera + mic) and
`{identity}/screen` (screenshare; its announce/unannounce is the share
lifecycle). Capture and encode stay in [`moq-video`](/lib/rs/moq-video) and
[`moq-audio`](/lib/rs/moq-audio).

```bash
cargo add moq-room
```

```rust
use moq_net::{Origin, Path};
use moq_room::{Kind, Room, claims};

let token = key.sign(&claims("meet/demo", "alice"), None)?;
let origin = Origin::random().produce();
let mut room = Room::new(&origin.consume(), Some(Path::new("alice").to_owned()));
while let Some(event) = room.next().await {
    if event.kind == Kind::Camera {
        // subscribe to event.broadcast
    }
}
```

The ordered UTF-8 `chat` track (from iroh-live's `iroh-rooms`) is
`moq_room::chat`. That is not hang.live's `hang/chat.json` catalog extension.

Gossip, tickets, and 1:1 Call stay in iroh-live. API:
[docs.rs/moq-room](https://docs.rs/moq-room).
