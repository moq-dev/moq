# [S] The Node finish reader exits once it has printed its marker

## Goal

`finish <publisher> -> js-native-node` passes every run: the Node reader exits as soon as it has
printed `finish clean ...`, so its row is never failed by `timeout`.

## Plan

Seen once, unloaded, in `just test interop --publishers python,go,rust --subscribers
cpp,c,gst,js-native-node,js-native-bun,rust` on `main` at `03918cbae5` (2026-11-27):
`finish go -> js-native-node` failed `exited 124`, which failed the rest of the row as
`reader failed: js-native-node`. Its log shows the whole case passing first:

```
finish clean frames=10 catalog=eof announce=end health=ok
subscribe close: id=2 broadcast=finish-go-.../fixture.hang.health track=live
```

Not reproduced since, in 1,146 Node finish-reader exits on 2026-11-28, on a host loaded to a
load average of 25 to 80:

- 1,077 from a scratch loop of the finish lane against one relay, 4 to 10 Node readers per
  publisher, Rust and Go publishers. Session close took at most 39 ms and the transport's
  `closed` at most 103 ms after it.
- 45 from 15 runs of `interop.sh --finish --publishers python,go,rust --subscribers
  cpp,c,gst,js-native-node,js-native-bun,rust`, and 24 from 8 runs of the full lane above.

An open handle cannot be the cause: `subscribe.ts` always ends in `process.exit`, which does not
wait for handles. After the marker the reader only runs `connection.close()`, which its one
second drain deadline bounds, `await connection.closed`, and `process.exit`. So the hang is one
of:

- `connection.closed` never settling, which follows `@moq/web-transport`'s `closed`. In 0.1.6
  that waits for the native QUIC connection to close, which the polyfill's capsule close bounds
  at three RTTs or 100 ms.
- `process.exit` blocking in native teardown: Node joins its libuv threadpool and V8 platform
  threads before `exit()`, while napi-rs's Tokio workers keep running (the same teardown race
  segfaults Bun after a frame, which `subscribe.ts` already notes).

The reader now prints `session closed` once `connection.closed` settles, so the next occurrence
names its side. With that line, the hang is in teardown: capture a native stack of a hung reader
(`eu-stack -p`) and take it to [moq-dev/web-transport](https://github.com/moq-dev/web-transport)
or Node. Without it, the close never completed: find what the transport's `closed` waits on.

Public API: none. Wire: none.

## Parked

- until the hang recurs - 1,146 exits did not reproduce it, so resume once an interop log shows `finish <publisher> -> js-native-node (exited 124)` after `finish clean`, reading whether `session closed` follows the marker
