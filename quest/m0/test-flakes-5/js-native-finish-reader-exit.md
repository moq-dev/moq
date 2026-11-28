# [S] The Node finish reader exits once it has printed its marker

## Goal

`finish <publisher> -> js-native-node` passes every run: the Node reader exits as soon as it has
printed `finish clean ...`, so its row is never failed by `timeout`.

## Plan

Seen once, unloaded, in `just test interop --publishers python,go,rust --subscribers
cpp,c,gst,js-native-node,js-native-bun,rust` on `main` at `03918cbae5`:
`finish go -> js-native-node` failed `exited 124`, which failed the rest of the row as
`reader failed: js-native-node`. Its log shows the whole case passing first:

```
finish clean frames=10 catalog=eof announce=end health=ok
subscribe close: id=2 broadcast=finish-go-.../fixture.hang.health track=live
```

So the reader finished its checks and then hung on the way out, until the harness's limit.
Find what it still waits on after the marker (the session close, the stdin barrier, or a handle
the WebTransport polyfill keeps), in `test/interop/clients/js-native/subscribe.ts`'s finish path
and `finish.ts`, and close it. Reproduce with `just test interop --finish` looped through
`quest gate`, under load if it does not show alone.

Public API: none. Wire: none.
