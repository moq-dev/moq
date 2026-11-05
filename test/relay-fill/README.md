# Relay fills

`just test relay-fill` puts a JavaScript relay (`relay.ts`) between a Rust publisher and a
Rust subscriber, over real WebSocket/QMux sessions on lite-05, lite-06, and lite-07. The
relay's copy FETCHes upstream the groups a Rust reader behind it misses:

- below where its feed started, by the copy's start rule: a floor the reader names before
  lite-06, the reader's own budget on lite-07, and the publisher's re-offer alone on lite-06;
- inside what it received, once its manual clock ages those groups out of its cache.

The relay uses Bun and the source checkout. Every listener binds an OS-assigned port. The
interop workflow runs it; normal Rust CI skips it.
