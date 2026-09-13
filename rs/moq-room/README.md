# moq-room

Headless multi-participant rooms over [Media over QUIC](https://moq.dev/). A room is a path prefix. There is no service and no storage: joining is minting a moq-token rooted at that prefix.

Participants are discovered from the announce stream. Identity is the path before `camera.hang` / `screen.hang`. Each participant publishes `{identity}/camera.hang` and `{identity}/screen.hang`.

This is the native counterpart of [`@moq/room`](https://www.npmjs.com/package/@moq/room). hang.live and [iroh-live](https://github.com/n0-computer/iroh-live) (`iroh-rooms` is being redesigned onto the announce bus) can depend on it for roster, path convention, token claims, and the `chat` track. Gossip, tickets, and 1:1 Call stay in iroh-live. Capture/encode stay in `moq-video` / `moq-audio`. Native `Local`/`Remote` media plumbing stays with those crates too; this crate is media-free.

```rust
use moq_net::{Origin, Path};
use moq_room::{Kind, Room, chat, claims};

let token = key.sign(&claims("meet/demo", "alice")?, None)?;
// Dial the relay at meet/demo?jwt=...

let origin = Origin::random().produce();
let mut room = Room::new(&origin.consume(), Some(Path::new("alice").to_owned()));
while let Some(event) = room.next().await {
    if event.kind == Kind::Camera {
        if let Some(broadcast) = &event.broadcast {
            if let Ok(mut chat) = chat::Subscriber::subscribe(broadcast).await {
                // ...
            }
        }
    }
}
```

Chat uses `moq-json::window` with ten seconds of history. Drive
`Publisher::expire()` alongside incoming sends (or call `poll_expire` from a poll
loop) so messages expire while idle. `Subscriber::recv` returns push/pop/skip
events and propagates errors. The format differs from iroh-live's raw UTF-8 chat.
