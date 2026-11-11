# Room flow

`Room.tla` is a TLA+ model of you and Bob in a room, written to be drawn as a statechart with
`/statechart`. Your side is `src/local.ts`: joining announces your camera broadcast, and a live
screen share announces your screen broadcast while joined. Your view of Bob is `src/room.ts`: each of his
announcements adds or removes one of his broadcasts, and your connection dropping removes him
until the reconnect announces him again.

| Rule | Holds when |
| --- | --- |
| `LeavingStopsPublishing` | Someone who has not joined the room, or has left it, publishes nothing to it. |

The chart also shows every participant leaving your screen while your connection is down and
coming back once it reconnects.

## Run

```sh
just js tla room/verify/Room.tla
```

To draw the chart, run `/statechart` with
`Room.tla`, `Room.cfg`, and `Room.view.ts`. `Room.cfg` plays only Bob's camera; add `"screen"` to
`Kinds` to play his share too.

## Left out

Chat, typing, and user metadata, more than one remote participant, a participant restarting under
the same identity, identity and prefix changes, and anything an app does on top of `Local`.
