# [M] @moq/net mirrors moq-net in names, units, and one way per task

## Goal

`@moq/net` 0.4 is the shape a consumer learns once: an error family under
one namespace with Rust's names, one way to open a connection, one way to
turn a path into a broadcast, one duration type, and no wire-layer method
on the handles an application holds.

## Plan

- Errors: `Net.Error.{Session, Stream, NotFound, TooFarBehind, FrameTooLarge,
  GroupTooLarge, ProtocolViolation}` beside `SessionCode`/`StreamCode`.
  Today `Lagged` names `StreamCode.TooFarBehind`, `NotFound` is at the
  root, three classes hang off `Group`, and `ProtocolViolation` is thrown
  but not exported.
- Connection: `Connection.connect({ url, ...props })` and
  `Connection.accept({ transport, url, ...props })` take the props object
  the constructor takes; re-export the session type as
  `Connection.Established`, which `Announce.BroadcastProps.connection` names
  in public but nobody can write; `Connection.Delay` becomes
  `Connection.Backoff` to match `moq_tokio::Backoff`; one suffix across
  `WebTransportProps`/`WebSocketOptions`/`ConnectionProps`.
- Origin: JS `Origin.BroadcastRequest` (the handler side) becomes
  `Origin.Request` as in Rust, and the consumer handle takes the name the
  Rust one settles on in [origin scoping](/quest/m1/api-net-origin.md);
  `dynamic()` stops accepting a bare string where `announced()` does not.
- Path to broadcast: `origin.request(path, { announced?: boolean })` is the
  one call; `Announce.Broadcast`, `Established.consume`, and
  `announcedBroadcast` fold into it and `Announce.Broadcast.closed`
  (`Promise<void>` where every other handle is `GetPromise<Error | null>`)
  goes with them.
- Durations: `Track.Info.maxAge`, `Subscription.maxAge`,
  `WebSocketOptions.delay`, and `DEFAULT_MAX_AGE_MS` are `Time.Milli` like
  `linger`, `Delay.*`, and `rtt`.
- Bounds: `Subscription.groups?: Groups` replaces `startGroup`/`endGroup`
  (exclusive) beside `setGroups({ start: { included } })`; Rust has one
  `RangeBounds` shape for both.
- Internal: `Broadcast.Producer/Consumer.{subscribe, resolveTrackInfo,
  fetchGroup, requested}` and `Track.Broadcast` are `@internal` (they are
  documented "used by the wire layer"); so are `Announce.Producer`,
  `Bandwidth.Want`/`allocate`, and `Track.Consumer`'s constructor.
- Delete `NO_DISCOVERY_HOSTS` in `connection/connect.ts`; a hardcoded
  hostname list in a library is a caller-side `discovery: false`.
- Bugs in the same files: a second handle's `linger` is silently ignored on
  a shared connection (refuse or take the max); a partial private `delay`
  inherits the 10 s default timeout where none means retry forever.

Public API: breaking on @moq/net, so on dev. Wire: none. Consumers:
@moq/hang, @moq/watch, @moq/publish, @moq/room, `demo/web`, moq.pro's app.

## Related

- [Published types](/quest/m0/js-net-internal-types.md) - the `@internal` tag that already strips a referenced type
- [@moq/net additive](/quest/m2/js-net-additive.md) - the iterators and getters that follow on main
- [Broadcast route](/quest/m2/js-broadcast-route.md) - what `announced: true` matches
