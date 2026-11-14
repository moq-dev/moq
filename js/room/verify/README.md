# Room flow

`Room.tla` is a TLA+ model of you and Bob in a room.
Your side is `src/local.ts`: joining announces your camera broadcast, and a live
screen share announces your screen broadcast while joined. Your view of Bob is `src/room.ts`: each of his
announcements adds or removes one of his broadcasts, and your connection dropping removes him
until the reconnect announces him again.

| Rule | Holds when |
| --- | --- |
| `LeavingStopsPublishing` | Someone who has not joined the room, or has left it, publishes nothing to it. |

Every participant also leaves your screen while your connection is down and
comes back once it reconnects.

## Run

```sh
quest gate just js tla room/verify/Room.tla
```

`Room.cfg` plays only Bob's camera; add `"screen"` to
`Kinds` to play his share too.

## Left out

Chat, typing, and user metadata, more than one remote participant, a participant restarting under
the same identity, identity and prefix changes, and anything an app does on top of `Local`.
